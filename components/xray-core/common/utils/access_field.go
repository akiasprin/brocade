package utils

import "reflect"

// AccessField can used to access unexported field of a struct
// valueType must be the exact type of the field or it will panic
func AccessField[valueType any](obj any, fieldName string) *valueType {
	value, ok := TryAccessField[valueType](obj, fieldName)
	if !ok {
		panic("field " + fieldName + " is missing, unaddressable, or has an unexpected type")
	}
	return value
}

// TryAccessField resolves an addressable private field without converting its
// address through uintptr. It returns false when a dependency changes its
// private layout, allowing performance paths to fail closed instead of
// corrupting memory or tripping checkptr.
func TryAccessField[valueType any](obj any, fieldName string) (*valueType, bool) {
	object := reflect.ValueOf(obj)
	if !object.IsValid() || object.Kind() != reflect.Pointer || object.IsNil() {
		return nil, false
	}
	element := object.Elem()
	if element.Kind() != reflect.Struct {
		return nil, false
	}
	field := element.FieldByName(fieldName)
	if !field.IsValid() || !field.CanAddr() || field.Type() != reflect.TypeOf(*new(valueType)) {
		return nil, false
	}
	// Keep the pointer associated with its allocation. Converting UnsafeAddr's
	// uintptr back to a pointer makes checkptr treat field access as detached
	// pointer arithmetic, even though reflect already resolved the field.
	return (*valueType)(field.Addr().UnsafePointer()), true
}
